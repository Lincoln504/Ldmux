//! The C ABI over the phone's radio route. Header: `packages/mobile/native/ldmux_client.h`,
//! which is the contract; read it for ownership rules.
//!
//! Rules this file keeps, each one checkable:
//!
//! - No global mutable state. Every piece of state is behind a handle the caller owns.
//! - No pointer into Rust memory ever leaves this crate to be interpreted. Handles are opaque
//!   and only passed back; every byte the caller receives is copied into a buffer *it* gave.
//! - Every entry point returns a status code and never unwinds across the boundary (a panic
//!   becomes [`LDMUX_ERR_PANIC`]).
//! - Destroy functions take a pointer *to* the handle and null it, so a second destroy is a
//!   no-op and a use after destroy is [`LDMUX_ERR_NULL`] instead of undefined behaviour.
//!
//! The handles are not thread safe; the caller serialises calls on one handle.
#![allow(clippy::missing_safety_doc)]
// Closures passed to `with_session` run inside the caller's `unsafe` block, so the inner blocks
// are redundant to the compiler; they stay because each one carries its own SAFETY argument.
#![allow(unused_unsafe)]

use std::panic::{AssertUnwindSafe, catch_unwind};

/// The phone's client engine: the whole radio route behind one handle.
pub mod client;

pub use client::*;

/// `power_dbm` meaning "leave the radio's transmit power alone", for
/// [`ldmux_client_set_preset`].
pub const LDMUX_UNSET_POWER: i8 = -128;

// **The numbers are the header's**, because they cross the ABI: `ldmux_client.h` is what a caller
// reads and `NativeRadioClient.kt` is what a caller compares against, so a value invented here is a
// code the app misreads. `LDMUX_ERR_*` is append-only for the same reason.
pub const LDMUX_OK: i32 = 0;
pub const LDMUX_ERR_NULL: i32 = -1;
pub const LDMUX_ERR_PANIC: i32 = -2;
pub const LDMUX_ERR_ARG: i32 = -3;
pub const LDMUX_ERR_BUFFER: i32 = -4;
pub const LDMUX_ERR_TOO_LONG: i32 = -24;
pub const LDMUX_ERR_DECODE: i32 = -26;
pub const LDMUX_ERR_QUEUE_FULL: i32 = -30;
pub const LDMUX_ERR_NOT_OPEN: i32 = -31;
pub const LDMUX_ERR_TOO_MANY_PAGES: i32 = -32;
pub const LDMUX_ERR_ALREADY_OPEN: i32 = -33;
pub const LDMUX_ERR_NO_PAGE: i32 = -34;
// The handshake codes: the v14 ABI's numbers, kept because a pairing probe still reports them.
pub const LDMUX_ERR_MALFORMED: i32 = -20;
pub const LDMUX_ERR_BAD_FILTER: i32 = -21;
pub const LDMUX_ERR_STALE: i32 = -22;
pub const LDMUX_ERR_REPLAY: i32 = -23;
pub const LDMUX_ERR_PIN_MISMATCH: i32 = -25;
pub const LDMUX_ERR_CRYPTO: i32 = -27;

/// A handshake failure as a status code, for the one caller that still handshakes in Kotlin terms:
/// the pairing probe, which asks a radio whether the keys are right before a session exists.
pub(crate) fn hs_err(e: ldmux_session::handshake::HandshakeError) -> i32 {
    use ldmux_session::handshake::HandshakeError as H;
    match e {
        H::Malformed => LDMUX_ERR_MALFORMED,
        H::BadFilter => LDMUX_ERR_BAD_FILTER,
        H::Stale => LDMUX_ERR_STALE,
        H::Replay => LDMUX_ERR_REPLAY,
        H::PinMismatch => LDMUX_ERR_PIN_MISMATCH,
        H::Crypto => LDMUX_ERR_CRYPTO,
    }
}

/// Run a body behind `catch_unwind`, turning a panic into a status code.
fn guard(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(LDMUX_ERR_PANIC)
}
