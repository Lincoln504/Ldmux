//! `ldmux-vt-sys` — hand-written FFI over the vendored `libghostty-vt`.
//!
//! The raw [`ffi`] module is the `extern "C"` surface (the one generic accessor
//! plus lifecycle, bound by hand against the vendored headers); the
//! ergonomic named accessors live in the public [`safe`] wrapper. See
//! `docs/RUST-PORT-PLAN.md` §3.4 / decision D2.

pub mod ffi;
mod safe;

pub use safe::{Cell, CellStyle, FfiError, RenderState, StyleColor, Terminal};
