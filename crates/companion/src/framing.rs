//! Framing for the MeshCore companion protocol — **re-exported from `ldmux-mesh`**.
//!
//! The bytes are the same whether they travel over a USB port or a BLE characteristic, and the phone
//! has to link this code without linking a host runtime, so the implementation lives in
//! `ldmux_mesh::companion::framing` (`docs/RADIO-PLAN.md` §8.1, `IC-4`). This module stays so that
//! `crate::framing::…` keeps naming the thing it always named; it defines nothing of its own.

pub use ldmux_mesh::companion::framing::*;
