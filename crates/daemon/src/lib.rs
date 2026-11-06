//! The ldmux daemon: registry, ownership, remote WS, streams, desktop,
//! inventory, the CLI (`bin/ldmux`), and the daemon entry (`bin/ldmuxd`).
//!
//! Ported from `packages/daemon` (the TS original); each module names
//! its TS source in its own doc comment. **Those files no longer exist** — the
//! TypeScript tree was deleted at D4; read them at git commit `6b3efb8`, the
//! last commit that carried the tree.
//!
//! - [`auth`] — the remote access token (0600 file, constant-time compare)
//! - [`qr`] — the QR encoder for `ldmux code` (ISO/IEC 18004, v1–40)
//! - [`sanitize`] — the stateful pane-byte sanitizer (OSC 52, DCS/APC, DA/DSR)
//! - [`ownership`] — who is looking at a session (ADR-0010)
//! - [`registry`] — the durable session index
//! - [`cwd`] — where a session's directory comes from
//! - [`proc`] / [`exec`] — async spawn, both pipes drained, group-kill, timeout
//! - [`pairing`] — the pairing for the address this daemon listens on
//!
//! The TS original of every module is the named `packages/daemon/src/…`
//! TS file; the tests beside each are its TS test file, ported 1:1. The daemon
//! proper (remote WS, streams, desktop, CLI, keeper client) lands in later
//! phases (plan §7).
//!
//! See `docs/ARCHITECTURE.md` for the design of record.

pub mod auth;
pub mod clipboard_policy;
pub mod control_socket;
pub mod cwd;
pub mod daemon;
pub mod daemon_lock;
pub mod desktop;
pub mod desktop_inventory;
pub mod exec;
pub mod fake_keeper;
pub mod keeper_supervisor;
pub mod mesh;
pub mod mesh_bridge;
pub mod mesh_serial;
pub mod mesh_session;
pub mod ownership;
pub mod pairing;
pub mod proc;
pub mod qr;
pub mod registry;
pub mod remote;
pub mod sanitize;
pub mod scope;
pub mod stream;
pub mod ws;
