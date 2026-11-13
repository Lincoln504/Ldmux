//! The companion radio's command window, shared by both ends of the link.
//!
//! The host drives a MeshCore companion over USB serial (`crates/companion`) and the phone drives
//! its own over Bluetooth LE. Both speak the same command protocol, both are limited by the same
//! single answer window, and both were, until now, written twice: the host's serial supervisor in
//! Rust and the phone's `MeshcoreSession` in Java. The one-command-at-a-time rule lived only in the
//! second, and only partly — which is how a lookup's `ERR_CODE_NOT_FOUND` came to settle a raw send
//! (`RC-2`).
//!
//! What is here is the rule itself, as state with no I/O and no clock: [`fifo`]. Everything that
//! names an opcode — the command table, the framing, the read loop — stays with the crate that owns
//! a radio, because the host and the phone issue different commands.

pub mod build;
pub mod command;
pub mod fifo;
pub mod framing;
pub mod push;
pub mod route;

pub use fifo::{
    Answer, CommandId, CommandSpec, CompanionFifo, DEFAULT_DRAIN_MS, DEFAULT_TIMEOUT_MS, Entry,
    Enqueue, Expired, Settled, UNIVERSAL_ANSWERS,
};
