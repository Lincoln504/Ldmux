//! `ldmux-vt-sys` — hand-written FFI over the vendored `libghostty-vt`.
//!
//! The raw [`ffi`] module is the `extern "C"` surface (the one generic accessor
//! plus lifecycle, bound by hand against the vendored headers); the
//! ergonomic named accessors live in the public [`safe`] wrapper. See
//! decision D2.

pub mod ffi;
pub mod links;
mod safe;
pub mod sgr;
pub mod snapshot;

pub use safe::{
    Cell, CellStyle, FfiError, MODE_APP_CURSOR, MODE_BRACKETED_PASTE, MODE_SGR_MOUSE, RenderState,
    ScrollViewport, StyleColor, Terminal,
};
